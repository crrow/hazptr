use std::{
    collections::HashSet,
    marker::PhantomData,
    sync::atomic::{AtomicBool, AtomicIsize, AtomicPtr, AtomicU64, AtomicUsize, Ordering},
};

use crate::{asymmetric_heavy_barrier, asymmetric_light_barrier, HeavyBarrierKind};
use crate::{Deleter, HazPtr, Reclaim};

const SYNC_TIME_PERIOD: u64 = std::time::Duration::from_nanos(2_000_000_000).as_nanos() as u64;
const RETIRED_COUNT_THRESHOLD: isize = 1000;
const HAZPTR_COUNT_MULTIPLER: isize = 2;

// At present, the domain is static which managed by us,
// so we can derefer the raw pointer freely;
// Things changed once we have different domain, since
// once the domain is dropped, then all the hazptrs should
// also be dropped.
//
//
// TODO: fix the domain.

// make the struct backwards compatible
#[non_exhaustive]
pub(crate) struct Global; // so no one can create a Global Domain by someone else.
static SHARED_DOMAIN: HazPtrDomain<Global> = HazPtrDomain::new();

#[macro_export]
macro_rules! domain {
    () => {
        HazPtrDomain::<()>::new()
    };
}

// Holds linked list of HazPtrs.
//
// One for allocating, one for retiring.
pub struct HazPtrDomain<F> {
    hazptrs: HazPtrs,
    retired: RetiredList,
    // used for distincting different domain.
    family: PhantomData<F>,
    sync_time: AtomicU64,
    nbulk_reclaims: AtomicUsize,
}

impl HazPtrDomain<Global> {
    pub fn shared() -> &'static Self {
        &SHARED_DOMAIN
    }
}

impl<F> HazPtrDomain<F> {
    // But we also want user be albe to create their own domains.
    //
    // Most users don't need to specify custom domains and custom
    // deleters, and default use the default domain and default delters.
    // This is because when we do reclaim we don't actually reallocate the memory,
    // until the reclaim function is called again in the future.
    // So if one user uses its own domain and has a bunch of remove operations,
    // and then don't do any removes, then the memory will never be deallocated.
    // In the shared domain case, we will try to remove all removable data,
    // no matter what data structure that the user is using.
    // So in general, we should use the shared domain.
    pub const fn new() -> Self {
        Self {
            hazptrs: HazPtrs::new(),
            retired: RetiredList::new(),
            family: PhantomData,
            sync_time: AtomicU64::new(0),
            nbulk_reclaims: AtomicUsize::new(0),
        }
    }

    pub(crate) fn acquire(&self) -> &HazPtr {
        if let Some(hazptr) = self.try_acquire_existing() {
            return hazptr;
        }
        self.acquire_new()
    }

    fn try_acquire_existing(&self) -> Option<&HazPtr> {
        // get the head of the linkedlist.
        let head_ptr = &self.hazptrs.head;
        // unsafe derefer it.
        let mut node = head_ptr.load(Ordering::Acquire);

        // check if the pointer is null, if not, we check the active flag.
        // if the node is not null and it is active, we continue the loop.
        while !node.is_null() {
            let n = unsafe { &*node };
            if n.try_acquire() {
                return Some(n);
            }
            node = n.next.load(Ordering::Relaxed);
        }
        None
    }

    // try to acquire a hazard pointer from the domain.
    //
    // We need to walk through the linkedlist, and try to
    // find a node which is inactive. If we reach the end of
    // the linkedlist, which means there are no reuseable hazptr,
    // so we have to allocate one.
    fn acquire_new(&self) -> &'_ HazPtr {
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
        let mut head = self.hazptrs.head.load(Ordering::Acquire);
        // we use loop here since there may someone updating the new head at the same
        // time.
        loop {
            // set the next field of the node we just allocated as the head.
            // the new_hazptr is never be shared.
            *unsafe { &mut *new_hazptr }.next.get_mut() = head;
            // compare and swap the
            match self.hazptrs.head.compare_exchange_weak(
                head,
                new_hazptr,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                // finally we succed update the head,
                // and we can safe take the reference from the raw pointer.
                // since the node in the linkedlist is never deallocated.
                Ok(_) => {
                    self.hazptrs.count.fetch_add(1, Ordering::SeqCst);
                    break unsafe { &*new_hazptr };
                }
                // When CAS failed, it still us whats the head right now.
                // head_now: head is changed, try again with that as our next ptr.
                Err(head_now) => head = head_now,
            }
        }

        // // get the head of the linkedlist.
        // let head_ptr = &self.hazptrs.head;
        // // unsafe derefer it.
        // let mut node = head_ptr.load(Ordering::SeqCst);
        // let hazptr = loop {
        //     // check if the pointer is null, if not, we check the active flag.
        //     // if the node is not null and it is active, we continue the loop.
        //     while !node.is_null() && unsafe { &*node }.active.load(Ordering::SeqCst) {
        //         node = unsafe { &*node }.next.load(Ordering::SeqCst);
        //     }
        //     if node.is_null() {
        //     } else {
        //         // there maybe multiple threads try to acquire the same hazptr at the same
        //         // point, we need to guard against that.
        //         // safe: we never deallocate it.
        //         let node = unsafe { &*node };
        //         if node
        //             .active
        //             .compare_exchange_weak(false, true, Ordering::SeqCst, Ordering::SeqCst)
        //             .is_ok()
        //         {
        //             // swap succeed, its ours
        //             break node;
        //         }
        //         // someone else grabbed the node right before us, keep walking.
        //         // TODO: there are existing optimization: allocating node if we
        //         // fail too many times.
        //     }
        // };
        // hazptr
    }
    // We cannot implement the retire on generic user type, because then
    // we will stick on the particular type, and have multiple domain type
    // for each type is not that good.
    //
    // retire needs to pass the deleter into the domain.
    pub(crate) fn retire(&self, ptr: *mut dyn Reclaim, d: &'static dyn Deleter) {
        // First stick ptr onto the list of retired.
        let new_retired = Box::into_raw(Box::new(Retired {
            data_ptr: ptr,
            deleter: d,
            next: AtomicPtr::new(std::ptr::null_mut()),
        }));

        crate::asymmetric_light_barrier();

        let head_ptr = &self.retired.head;
        let mut head = head_ptr.load(Ordering::Acquire);
        loop {
            *unsafe { &mut *new_retired }.next.get_mut() = head;
            match head_ptr.compare_exchange_weak(
                head,
                new_retired,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(new_old_head) => head = new_old_head,
            };
        }

        // Increment the count before we give anyone a chance to reclaim it.
        // we need to update the counter before we actually update the list.
        // because someone else may decrement the count, including our own
        // before we add. then the count may be negative.
        self.retired.count.fetch_add(1, Ordering::Release);

        // try reclaim
        self.check_cleanup_and_reclaim();
    }

    fn check_cleanup_and_reclaim(&self) {
        if self.try_timed_cleanup() {
            return;
        }
        if Self::reached_threshold(
            self.retired.count.load(Ordering::Acquire),
            self.hazptrs.count.load(Ordering::Acquire),
        ) {
            self.try_bulk_reclaim();
        }
    }

    fn try_timed_cleanup(&self) -> bool {
        if !self.check_sync_time() {
            return false;
        }
        self.relaxed_cleanup();
        true
    }

    fn relaxed_cleanup(&self) {
        self.retired.count.store(0, Ordering::Release);
        self.bulk_reclaim(true);
    }

    fn check_sync_time(&self) -> bool {
        use std::convert::TryFrom;
        let time = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("current time is set to before the epoch")
                .as_nanos(),
        )
        .expect("system time is too far into the future");

        let sync_time = self.sync_time.load(Ordering::Relaxed);
        // if it's not time to clean yet, or someone just starting cleaning, don't clean.
        time > sync_time
            && self
                .sync_time
                .compare_exchange(
                    sync_time,
                    time + SYNC_TIME_PERIOD,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
    }

    const fn reached_threshold(rc: isize, hc: isize) -> bool {
        rc >= RETIRED_COUNT_THRESHOLD && rc >= HAZPTR_COUNT_MULTIPLER * hc
    }

    fn try_bulk_reclaim(&self) {
        let hc = self.hazptrs.count.load(Ordering::Acquire);
        let rc = self.retired.count.load(Ordering::Acquire);
        if !Self::reached_threshold(rc, hc) {
            return;
        }
        self.retired.count.swap(0, Ordering::Release);
        if !Self::reached_threshold(rc, hc) {
            return;
        }
        self.bulk_reclaim(false);
    }

    fn bulk_reclaim(&self, transitive: bool) -> usize {
        self.nbulk_reclaims.fetch_add(1, Ordering::Acquire);

        let mut reclaimed = 0;

        loop {
            // we need to steal the entire retired linked list.
            let steal = self
                .retired
                .head
                .swap(std::ptr::null_mut(), Ordering::SeqCst);

            asymmetric_heavy_barrier(HeavyBarrierKind::Expedited);

            if steal.is_null() {
                // nothing to reclaim.
                return reclaimed;
            }

            // get all guarded addresses.
            // walking the hazptr linked list, and collect all address into the hashset
            // collection.
            let mut guarded_ptrs = HashSet::new();
            let mut node = self.hazptrs.head.load(Ordering::Acquire);
            while !node.is_null() {
                // Safety: we haven't deallocated any node while the domain lives.
                let n = unsafe { &*node };
                // NOTE: folly doesn't skip the active node here.
                if n.active.load(Ordering::SeqCst) {
                    // only when the ptr has been actived, then we insert it into the guarded list.
                    guarded_ptrs.insert(n.ptr.load(Ordering::Acquire));
                }
                node = n.next.load(Ordering::Relaxed);
            }

            let (reclaimed_now, done) = self.bulk_lookup_and_reclaim(steal, guarded_ptrs);
            reclaimed += reclaimed_now;

            if done || !transitive {
                break;
            }
        }
        self.nbulk_reclaims.fetch_sub(1, Ordering::Release);
        reclaimed
    }

    fn bulk_lookup_and_reclaim(
        &self,
        stolen_retired_head: *mut Retired,
        guarded_ptrs: HashSet<*mut u8>,
    ) -> (usize, bool) {
        // reclaim any retired objects that aren't guarded.
        let mut node = stolen_retired_head;
        let mut remaining = std::ptr::null_mut();
        let mut tail = None;
        let mut reclaim_count = 0;
        let mut still_retired_count = 0;

        while !node.is_null() {
            // Safety: we own these objects since we steal the linkedlist's head at the
            // begining. so no one can access the linedlist.
            let n = unsafe { &*node };
            let next = n.next.load(Ordering::Relaxed); // update the current ptr
            debug_assert_ne!(node, next);

            // compare the data address.
            if !guarded_ptrs.contains(&(n.data_ptr as *mut u8)) {
                // no longer guarded, use deleter to reclaim.
                //
                // current has no hazard point guarding it, so we only have remainly pointer.
                let n = unsafe { Box::from_raw(node) };
                unsafe {
                    n.deleter.delete(n.data_ptr);
                }

                // TODO: support linked nodes for more efficient deallocation.

                reclaim_count += 1;
            } else {
                // not safe for reclaiming

                // if the address is still guareded, then its not safe to reclaim.
                // we need to reconstruct the list

                // make current node point to the remaining.
                n.next.store(remaining, Ordering::Relaxed);
                // *stealed_retired_node.next.get_mut() = remaining;
                // make current node as the head of remaining.
                remaining = node;
                if tail.is_none() {
                    tail = Some(remaining);
                }
                still_retired_count += 1;
            }
            node = next;
        }

        let done = self.retired.head.load(Ordering::Acquire).is_null();

        // update the retired list count.
        self.retired
            .count
            .fetch_sub(reclaim_count, Ordering::SeqCst);

        // we have to reconstruct the retire linkedlist

        let tail = if let Some(tail) = tail {
            assert!(!remaining.is_null());
            assert_ne!(still_retired_count, 0);
            tail
        } else {
            // nothing remain, return.
            assert!(remaining.is_null());
            assert_eq!(still_retired_count, 0);
            return (reclaim_count as usize, done);
        };

        asymmetric_light_barrier();

        // try to write the remaining linkedlist back to the retired linkedlist.
        let original_retired_list = &self.retired.head;
        let mut head = original_retired_list.load(Ordering::Acquire);
        loop {
            *unsafe { &mut *tail }.next.get_mut() = head;
            match original_retired_list.compare_exchange_weak(
                head,
                remaining,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(head_now) => head = head_now,
            }
        }

        self.retired
            .count
            .fetch_add(still_retired_count, Ordering::SeqCst);

        (reclaim_count as usize, done)
    }

    pub fn eager_reclaim(&self, block: bool) -> usize {
        self.bulk_reclaim(true)
    }

    pub(crate) fn release(&self, hazard: &HazPtr) {
        hazard.release()
    }
}

impl<F> Drop for HazPtrDomain<F> {
    fn drop(&mut self) {
        // There should be no hazard pointers active.
        let nretired = *self.retired.count.get_mut();
        let nreclaimed = self.bulk_reclaim(false);
        debug_assert_eq!(
            nretired, nreclaimed as isize,
            "no one should hold ptr any more"
        );
        assert!(
            self.retired.head.get_mut().is_null(),
            "nothing should on the retired list any more"
        );

        // also drop all hazptrs, as no-one should be holding them any more.
        let mut cur_node = *self.hazptrs.head.get_mut();
        while !cur_node.is_null() {
            // we are in drop, get the exclusive reference.
            let mut n = unsafe { Box::from_raw(cur_node) };
            assert!(!*n.active.get_mut()); // if we are exclusive borrowing, just use get_mut.
            cur_node = *n.next.get_mut();
            drop(n);
        }
    }
}

/// In realistic we may have a basic linkedlist type for the following two
/// Linkedlist, and also, they two are also have some slight difference for
/// optimization, so its fine for us just inline implementing they now.

// The allocating linkedlist.
struct HazPtrs {
    head: AtomicPtr<HazPtr>,
    count: AtomicIsize,
}
impl HazPtrs {
    const fn new() -> Self {
        Self {
            head: AtomicPtr::new(std::ptr::null_mut()),
            count: AtomicIsize::new(0),
        }
    }
}

// The retired linkedlist, once we retire some ptr, then it should go here.
struct RetiredList {
    head: AtomicPtr<Retired>,
    // how many elements in the retired list?
    count: AtomicIsize,
}

impl RetiredList {
    const fn new() -> Self {
        Self {
            head: AtomicPtr::new(std::ptr::null_mut()),
            count: AtomicIsize::new(0),
        }
    }
}

// Each of the thing in the retired list, should certainly have the pointer,
// the vtable entry for the drop implementation of the target type.
struct Retired {
    // the actual pointer to the data, the thing we are going to compare with hazptrs'.
    data_ptr: *mut dyn Reclaim,
    // recalim should be a function pointer.
    deleter: &'static dyn Deleter,
    next: AtomicPtr<Retired>,
}
