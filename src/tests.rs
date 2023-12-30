#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicPtr, AtomicUsize, Ordering},
        Arc,
    };

    use crate::{
        deleter::deleters, domain::HazPtrDomain, HazPtrHolder, HazPtrObject, HazPtrObjectWrapper,
    };

    struct CountDrops(Arc<AtomicUsize>);
    impl Drop for CountDrops {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl CountDrops {
        fn new(x: usize) -> Self {
            CountDrops(Arc::new(AtomicUsize::new(x)))
        }
    }

    #[test]
    fn tfn() {
        let drops = Arc::new(AtomicUsize::new(0));
        let x = AtomicPtr::new(Box::into_raw(Box::new(
            HazPtrObjectWrapper::with_default_domain((1, CountDrops(drops.clone()))),
        )));

        // as a reader
        let mut h = HazPtrHolder::default();
        // safety:
        //    1. AtomicPtr point to a valid address which we just allocated.
        //    2. Writer to AtomicPtr use HazPtrObject::retire.
        let my_x = h.protect(&x).expect("not null");
        // valid:
        assert_eq!(my_x.0, 1i32);
        h.reset();
        // invalid:
        // let _: i32 = my_x.0;
        // reload
        let my_x = h.protect(&x).expect("not null");
        // valid:
        assert_eq!(my_x.0, 1i32);
        drop(h);
        // then invalid again
        // let _: i32 = my_x.0;

        // make a holder before writer
        let mut h = HazPtrHolder::default();
        let my_x = h.protect(&x).expect("not null");
        assert_eq!(1i32, my_x.0);

        // make a holder before writer
        let mut h_tmp = HazPtrHolder::default();
        let _ = h_tmp.protect(&x).expect("not null");
        drop(h_tmp);

        // as a writer try to retire
        let drops_2 = Arc::new(AtomicUsize::new(0));
        let old = x.swap(
            Box::into_raw(Box::new(HazPtrObjectWrapper::with_default_domain((
                2,
                CountDrops(drops_2.clone()),
            )))),
            Ordering::Acquire,
        );

        // build a holder after the writer
        let mut h2 = HazPtrHolder::default();
        let my_x2 = h2.protect(&x).expect("not null");
        assert_eq!(my_x2.0, 2);

        // safety: old value is not acessible.
        unsafe { HazPtrObject::retire(old, &deleters::drop_box) };
        // the origin perior reader can still read
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(1, my_x.0);

        let shared_domain = HazPtrDomain::shared();

        // still exists holder, so eager reclaim doesn't work
        assert_eq!(shared_domain.eager_reclaim(false), 0);

        assert_eq!(1, my_x.0);

        drop(h);
        assert_eq!(drops.load(Ordering::SeqCst), 0);

        assert_eq!(shared_domain.eager_reclaim(false), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(drops_2.load(Ordering::SeqCst), 0); // why ? since we doesn't retire the h2

        // check actually reclaimed

        drop(h2);
        assert_eq!(shared_domain.eager_reclaim(false), 0);
        assert_eq!(drops_2.load(Ordering::SeqCst), 0);
    }

    #[test]
    #[should_panic]
    fn feel_bad() {
        let dw = HazPtrDomain::<()>::new();
        let dr = HazPtrDomain::<()>::new();
        let _ = HazPtrDomain::shared();
        // FIX it, these are different domain instances.

        let drops_42 = Arc::new(AtomicUsize::new(0));
        let x = AtomicPtr::new(Box::into_raw(Box::new(HazPtrObjectWrapper::with_domain(
            &dw,
            (42, drops_42.clone()),
        ))));

        // as a reader
        // reader use a different domain than the writer.
        let mut h = HazPtrHolder::for_domain(&dr);
        let my_x = h.protect(&x).expect("should panic");
    }

    #[test]
    fn domain_macro() {
        let _ = domain!();
    }
}
