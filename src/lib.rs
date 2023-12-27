use std::{
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
};

// At present, the domain is static which managed by us,
// so we can derefer the raw pointer freely;
// Things changed once we have different domain, since
// once the domain is dropped, then all the hazptrs should
// also be dropped.
// TODO: fix the domain.
static SHARED_DOMAIN: HazPtrDomain = HazPtrDomain {
    hazptrs: HazPtrs {},
    retired: RetiredList {},
};

/// HazPtrHolder is used for readers.
///
/// Since we never deallocate the HazPtr, so it will be fine with static lifetime.
/// And every access is shared access.
///
/// HazPtrHolder should know where the HazPtrDomain come from.
#[derive(Default)]
pub struct HazPtrHolder(Option<&'static HazPtr>);

impl HazPtrHolder {
    fn hazptr(&mut self) -> &'static HazPtr {
        if let Some(ptr) = self.0 {
            ptr
        } else {
            // if we don't have ptr yet then we acquire one from the global domain.
            let ptr = SHARED_DOMAIN.acquire();
            self.0 = Some(ptr);
            ptr
        }
    }
    /// #Safety:
    ///    
    /// Caller must guarantee that the address in AtomicPtr is valid as a reference or null.
    ///     If it was null, then the address will be turned into an option through the [`std::ptr::NonNull::new`].
    /// Caller must also guarantee that the value behinde the AtomicPtr will only be deallocated
    /// through calls to [`HazPtrObject::retire`].
    ///
    /// The return type of &T is fine, since the lifetime is the [`HazPtrHolder`] it self, and the [`HazPtrObject`]
    /// will respect us.
    pub unsafe fn load<'l, T>(&'l mut self, ptr: &'_ AtomicPtr<T>) -> Option<&'l T> {
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
                    unsafe { nn.as_ref() }
                });
            } else {
                ptr1 = ptr2;
            }
        }
    }
    pub fn reset(&mut self) {
        // we can do this here since the reset require a mutable reference,
        // if there exists loaded value T, then we cannot call this method.
        if let Some(hp) = self.0 {
            hp.ptr.store(std::ptr::null_mut(), Ordering::SeqCst);
        }
    }
}

impl Drop for HazPtrHolder {
    fn drop(&mut self) {
        // make sure if is currently guarding something, then stop guarding that thing.
        self.reset();

        if let Some(hp) = self.0 {
            // inactive it, then other thread can reuse this thing.
            hp.active.store(false, Ordering::SeqCst);
        }
    }
}

pub struct HazPtr {
    // ptr is an AtomicPtr to self, because it should be writable by the thread that owns the hazptr,
    // and it should be readable by any writer thread.
    //
    // The ptr is a pointee points to another fat point.
    //
    ptr: AtomicPtr<u8>, // *mut u8
    next: AtomicPtr<HazPtr>,
    active: AtomicBool,
}

impl HazPtr {
    fn protect(&self, ptr: *mut u8) {
        // It should receive a shared reference, we should make sure it still valid.
        self.ptr.store(ptr, Ordering::SeqCst);
    }
}

pub trait Deleter {}

#[allow(dyn_drop, drop_bounds)]
pub trait HazPtrObject
where
    // COPY type cannot be DROP
    Self: Sized + Drop + 'static,
{
    fn domain(&self) -> &HazPtrDomain;

    /// Safety:
    ///
    ///  Caller must guarantee that pointer is still valid.
    ///  Caller must guarantee that Self is no longer accessible to readers.
    ///  Its okay for existing readers to still refer to Self.
    unsafe fn retire<D: Deleter>(me: *mut Self) {
        if !std::mem::needs_drop::<Self>() {
            return;
        }

        // we need to make sure the domain is still valid,
        // since the HazPtrObject represents user's data structure,
        // and the HazPtrHolder points to this Object,
        // at the same time, the domain hold the real PazPtr.
        // The dependency here is a little werid.
        //
        // HazPtrObj -> HazPtrDomain -> HazPtr
        // HazPtrObjectWrapper -> HazPtrDomain -> HazPtr
        let x = Self::domain(unsafe { &*me });
        // The reason we need the dyn cast here is that hazard pointer can
        // guard many different types, it doesn't care the underlying type,
        // but the domain is not a generic type.
        let _ = x.retire::<D>(me as *mut dyn Drop);
    }
}

pub struct HazPtrObjectWrapper<T> {
    inner: T,
    // domain: *const HazPtrDomain,
}

impl<T> HazPtrObjectWrapper<T> {
    pub fn with_default_domain(t: T) -> Self {
        Self {
            inner: t,
            // domain: &SHARED_DOMAIN,
        }
    }
}
impl<T> From<T> for HazPtrObjectWrapper<T> {
    fn from(value: T) -> Self {
        todo!()
    }
}

// TODO: get rid of this drop wrapper.
impl<T> Drop for HazPtrObjectWrapper<T> {
    fn drop(&mut self) {}
}

impl<T: 'static> HazPtrObject for HazPtrObjectWrapper<T> {
    fn domain(&self) -> &HazPtrDomain {
        &SHARED_DOMAIN
    }
}
impl<T> Deref for HazPtrObjectWrapper<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl<T> DerefMut for HazPtrObjectWrapper<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

// Holds linked list of HazPtrs.
//
// One for allocating, one for retiring.
pub struct HazPtrDomain {
    hazptrs: HazPtrs,
    retired: RetiredList,
}

impl HazPtrDomain {
    // try to acquire a hazard pointer from the domain.
    //
    // We need to walk through the linkedlist, and try to
    // find a node which is inactive. If we reach the end of
    // the linkedlist, which means there are no reuseable hazptr,
    // so we have to allocate one.
    fn acquire(&self) -> &'static HazPtr {
        // get the head of the linkedlist.
        let head_ptr = &self.hazptrs.head;
        // unsafe derefer it.
        let mut node = head_ptr.load(Ordering::SeqCst);
        let hazptr = loop {
            // check if the pointer is null, if not, we check the active flag.
            // if the node is not null and it is active, we continue the loop.
            while !node.is_null() && unsafe { &*node }.active.load(Ordering::SeqCst) {
                node = unsafe { &*node }.next.load(Ordering::SeqCst);
            }
            if node.is_null() {
                // no free hazptrs, allocate one
                let new_hazptr = Box::into_raw(Box::new(HazPtr {
                    ptr: AtomicPtr::new(std::ptr::null_mut()),
                    next: AtomicPtr::new(std::ptr::null_mut()),
                    // active is true, so no one else will try to use this pointer,
                    // after we stick it into the linkedlist.
                    active: AtomicBool::new(true),
                }));
                // stick it at the head of the linked list.
                // we load the head of the linkedlist outside the loop,
                // then the head will be updated by the returned new old value from the CAS.
                let mut head = head_ptr.load(Ordering::SeqCst);
                // we use loop here since there may someone updating the new head at the same time.
                break loop {
                    // set the next field of the node we just allocated as the head.
                    // the new_hazptr is never be shared.
                    *unsafe { &mut *new_hazptr }.next.get_mut() = head;
                    // compare and swap the
                    match head_ptr.compare_exchange_weak(
                        head,
                        new_hazptr,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    ) {
                        // finally we succed update the head,
                        // and we can safe take the reference from the raw pointer.
                        // since the node in the linkedlist is never deallocated.
                        Ok(_) => break unsafe { &*new_hazptr },
                        // When CAS failed, it still us whats the head right now.
                        // head_now: head is changed, try again with that as our next ptr.
                        Err(head_now) => head = head_now,
                    }
                };
            } else {
                // there maybe multiple threads try to acquire the same hazptr at the same point,
                // we need to guard against that.
                // safe: we never deallocate it.
                let node = unsafe { &*node };
                if node
                    .active
                    .compare_exchange_weak(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    // swap succeed, its ours
                    break node;
                }
                // someone else grabbed the node right before us, keep walking.
                // TODO: there are existing optimization: allocating node if we fail too many times.
            }
        };
        hazptr
    }
    // We cannot implement the retire on generic user type, because then
    // we will stick on the particular type, and have multiple domain type
    // for each type is not that good.
    //
    // retire needs to pass the deleter into the domain
    fn retire<D: Deleter>(&self, ptr: *mut dyn Drop) {
        // When compare ptr, only compare data, not vtable.
        todo!()
    }
}

/// In realistic we may have a basic linkedlist type for the following two
/// Linkedlist, and also, they two are also have some slight difference for
/// optimization, so its fine for us just inline implementing they now.

// The allocating linkedlist.
struct HazPtrs {
    head: AtomicPtr<HazPtr>,
}

// The retired linkedlist, once we retire some ptr, then it should go here.
struct RetiredList {
    head: AtomicPtr<Retired>,
}

struct Retired {
    // the actual pointer to the data, the thing we are going to compare with hazptrs'.
    data_ptr: *mut dyn Drop,
    reclaim: &'static dyn Fn(*mut dyn Drop),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tfn() {
        let x = AtomicPtr::new(Box::into_raw(Box::new(
            HazPtrObjectWrapper::with_default_domain(1),
        )));

        // as a reader
        let mut h = HazPtrHolder::default();
        // safety:
        //    1. AtomicPtr point to a valid address which we just allocated.
        //    2. Writer to AtomicPtr use HazPtrObject::retire.
        let my_x = unsafe { h.load(&x).expect("not null") };
        // valid:
        let _: i32 = **my_x;
        h.reset();
        // invalid:
        // let _: i32 = **my_x;
        // reload
        let my_x = unsafe { h.load(&x).expect("not null") };
        // valid:
        let _: i32 = **my_x;
        drop(h);
        // then invalid again
        // let _: i32 = **my_x;

        // as a writer
        let mut old = x.swap(
            Box::into_raw(Box::new(HazPtrObjectWrapper::with_default_domain(2))),
            Ordering::Acquire,
        );
        // safety: old value is not acessible.
        unsafe { HazPtrObject::retire(old) };
    }
}
