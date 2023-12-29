use std::{
    collections::HashSet,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
};

use crate::{Deleter, HazPtr, Reclaim};

// At present, the domain is static which managed by us,
// so we can derefer the raw pointer freely;
// Things changed once we have different domain, since
// once the domain is dropped, then all the hazptrs should
// also be dropped.
//
//
// TODO: fix the domain.
static SHARED_DOMAIN: HazPtrDomain = HazPtrDomain::new();

// Holds linked list of HazPtrs.
//
// One for allocating, one for retiring.
pub struct HazPtrDomain {
    hazptrs: HazPtrs,
    retired: RetiredList,
}

impl HazPtrDomain {
    pub fn shared() -> &'static Self {
        &SHARED_DOMAIN
    }
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
        }
    }
    // try to acquire a hazard pointer from the domain.
    //
    // We need to walk through the linkedlist, and try to
    // find a node which is inactive. If we reach the end of
    // the linkedlist, which means there are no reuseable hazptr,
    // so we have to allocate one.
    pub(crate) fn acquire(&self) -> &'_ HazPtr {
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
    // retire needs to pass the deleter into the domain.
    pub(crate) fn retire(&self, ptr: *mut dyn Reclaim, d: &'static dyn Deleter) {
        // First stick ptr onto the list of retired.
        let new_retired = Box::into_raw(Box::new(Retired {
            data_ptr: ptr,
            deleter: d,
            next: AtomicPtr::new(std::ptr::null_mut()),
        }));
        // Increment the count before we give anyone a chance to reclaim it.
        // we need to update the counter before we actually update the list.
        // because someone else may decrement the count, including our own
        // before we add. then the count may be negative.
        self.retired.count.fetch_add(1, Ordering::SeqCst);
        let head_ptr = &self.retired.head;
        let mut head = head_ptr.load(Ordering::SeqCst);
        loop {
            *unsafe { &mut *new_retired }.next.get_mut() = head;
            match head_ptr.compare_exchange_weak(
                head,
                new_retired,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(new_old_head) => head = new_old_head,
            };
        }

        // Sec, check if we need to retire
        if self.retired.count.load(Ordering::SeqCst) != 0 {
            // try to reclaim some objects
            // TODO:
            self.bulk_reclaim(0, false);
        }
    }

    fn bulk_reclaim(&self, pre_reclaimed_cnt: usize, block: bool) -> usize {
        // we need to steal the entire retired linked list.
        let steal = self
            .retired
            .head
            .swap(std::ptr::null_mut(), Ordering::SeqCst);
        if steal.is_null() {
            // nothing to reclaim.
            return 0;
        }

        // get all guarded addresses.
        // walking the hazptr linked list, and collect all address into the hashset collection.
        let mut guarded_ptrs = HashSet::new();
        let mut node = self.hazptrs.head.load(Ordering::SeqCst);
        while !node.is_null() {
            // Safety: we haven't deallocated any node.
            let n = unsafe { &*node };
            if n.active.load(Ordering::SeqCst) {
                // only when the ptr has been actived, then we insert it into the guarded list.
                guarded_ptrs.insert(n.ptr.load(Ordering::SeqCst));
            }
            node = n.next.load(Ordering::SeqCst);
        }

        // reclaim any retired objects that aren't guarded.
        let mut stealed_retired_node_ptr = steal;
        let mut remaining = std::ptr::null_mut();
        let mut retired_list_tail = None;
        let mut reclaim_count = 0;
        while !stealed_retired_node_ptr.is_null() {
            // Safety: we own these objects since we steal the linkedlist's head at the begining.
            // so no one can access the linedlist.
            let current = stealed_retired_node_ptr; // take the current node, otherwise, the linkedlist is broken
            let stealed_retired_node = unsafe { &*current };
            stealed_retired_node_ptr = stealed_retired_node.next.load(Ordering::SeqCst); // update the current ptr

            // compare the data address.
            if guarded_ptrs.contains(&(stealed_retired_node.data_ptr as *mut u8)) {
                // if the address is still guareded, then its not safe to reclaim.
                // we need to reconstruct the list

                // make current node point to the remaining.
                stealed_retired_node.next.store(remaining, Ordering::SeqCst);
                // *stealed_retired_node.next.get_mut() = remaining;
                // make current node as the head of remaining.
                remaining = current;
                if retired_list_tail.is_none() {
                    retired_list_tail = Some(remaining);
                }
            } else {
                // no longer guarded, use deleter to reclaim.
                //
                // current has no hazard point guarding it, so we only have remainly pointer.
                let n = unsafe { Box::from_raw(current) };
                unsafe {
                    n.deleter.delete(n.data_ptr);
                }
                reclaim_count += 1;
            }
        }

        // update the retired list count.
        self.retired
            .count
            .fetch_sub(reclaim_count, Ordering::SeqCst);
        let total_reclaim_count = reclaim_count + pre_reclaimed_cnt;

        // we have to reconstruct the retire linkedlist

        let tail = if let Some(tail) = retired_list_tail {
            assert!(!remaining.is_null());
            tail
        } else {
            // nothing remain, return.
            assert!(remaining.is_null());
            return total_reclaim_count;
        };

        // try to write the remaining linkedlist back to the retired linkedlist.
        let original_retired_list = &self.retired.head;
        let mut head = original_retired_list.load(Ordering::SeqCst);
        loop {
            *unsafe { &mut *tail }.next.get_mut() = head;
            match original_retired_list.compare_exchange_weak(
                head,
                remaining,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(head_now) => head = head_now,
            }
        }

        if !remaining.is_null() && block {
            // caller wants to reclaim everything, but have something remaining.
            std::thread::yield_now();
            return self.bulk_reclaim(total_reclaim_count, true);
        }
        total_reclaim_count
    }

    pub fn eager_reclaim(&self, block: bool) -> usize {
        self.bulk_reclaim(0, block)
    }
}

impl Drop for HazPtrDomain {
    fn drop(&mut self) {
        // There should be no hazard pointers active.
        let nretired = *self.retired.count.get_mut();
        let nreclaimed = self.bulk_reclaim(0, false);
        debug_assert_eq!(nretired, nreclaimed, "no one should hold ptr any more");
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
}
impl HazPtrs {
    const fn new() -> Self {
        Self {
            head: AtomicPtr::new(std::ptr::null_mut()),
        }
    }
}

// The retired linkedlist, once we retire some ptr, then it should go here.
struct RetiredList {
    head: AtomicPtr<Retired>,
    // how many elements in the retired list?
    count: AtomicUsize,
}

impl RetiredList {
    const fn new() -> Self {
        Self {
            head: AtomicPtr::new(std::ptr::null_mut()),
            count: AtomicUsize::new(0),
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
