mod deleter;
#[macro_use]
mod domain;
mod holder;
mod obj;
mod ptr;
mod tests;

pub use deleter::{deleters, Deleter, Reclaim};
pub use domain::HazPtrDomain;
pub use holder::HazPtrHolder;
pub use obj::{HazPtrObject, HazPtrObjectWrapper};
pub use ptr::HazPtr;

fn asymmetric_light_barrier() {
    std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
}

enum HeavyBarrierKind {
    Normal,
    Expedited,
}
fn asymmetric_heavy_barrier(kind: HeavyBarrierKind) {
    // TODO: if cfg(linux)
    std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
}


