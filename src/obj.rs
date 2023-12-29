//! Anything that guared by a HazPtr, needs to be dropped in a special way.
//! Basicly they need a sort of back reference to the domain, so we can know
//! when we gona drop the thing behind the atomic pointer, we need to point back
//! to the domain, so we gona know where to do the dropping, cause we need to retire
//! it and reclaim it, so we don't have to drop it in place.
//!
//! This is why we have the [`HazPtrObject`] which has the method [`HazPtrObject::retire`] ,
//! which is called when we want to remove something from the data structure.
//! And we also want people can implement the [`HazPtrObject`] themselves, so they
//! don't necessarily need the [`HazPtrObjectWrapper`].
//! But we still provide the type [`HazPtrObjectWrapper`] for who want it.
//!
use std::ops::{Deref, DerefMut};

use crate::{Deleter, HazPtrDomain, Reclaim};

pub trait HazPtrObject
where
    // COPY type cannot be DROP
    Self: Sized + 'static,
{
    fn domain(&self) -> &HazPtrDomain;

    /// Safety:
    ///
    ///  Caller must guarantee that pointer is still valid.
    ///  Caller must guarantee that Self is no longer accessible to readers.
    ///  Caller must guarantee that deleter is valid deleter for Self.
    ///  Its okay for existing readers to still refer to Self.
    unsafe fn retire(me: *mut Self, deleter: &'static dyn Deleter) {
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
        let _ = x.retire(me as *mut dyn Reclaim, deleter);
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
        crate::domain::HazPtrDomain::shared()
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
