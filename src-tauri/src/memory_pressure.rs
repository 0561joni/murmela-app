//! macOS memory-pressure notifications, used to drop idle model workers before
//! the system starts swapping.
use crate::review_jobs::ProcessingQueue;

/// Memory-pressure notifications. `mask` selects levels (1 normal, 2 warning,
/// 4 critical); the handler receives the level that fired.
pub(crate) struct MemoryPressureWatch(dispatch2::DispatchRetained<dispatch2::DispatchSource>);
struct PressureContext {
    source: *const dispatch2::DispatchSource,
    handler: Box<dyn Fn(usize) + Send + Sync>,
}
impl MemoryPressureWatch {
    pub fn new(queue: ProcessingQueue) -> Self {
        // Eviction uses the same lock as queue admission.
        Self::with_handler(0x2 | 0x4, move |_| queue.evict_idle())
    }
    pub fn with_handler(mask: usize, handler: impl Fn(usize) + Send + Sync + 'static) -> Self {
        use dispatch2::{DispatchObject, DispatchSource};
        extern "C" fn pressure(context: *mut std::ffi::c_void) {
            // The source owns this Box until its finalizer, which runs after
            // all handlers, and the source outlives its own handlers.
            unsafe {
                let context = &*context.cast::<PressureContext>();
                (context.handler)((*context.source).data());
            }
        }
        extern "C" fn finalize(context: *mut std::ffi::c_void) {
            unsafe {
                drop(Box::from_raw(context.cast::<PressureContext>()));
            }
        }
        unsafe {
            let source = DispatchSource::new(
                std::ptr::addr_of!(dispatch2::_dispatch_source_type_memorypressure).cast_mut(),
                0,
                mask,
                None,
            );
            let context = Box::new(PressureContext {
                source: &*source,
                handler: Box::new(handler),
            });
            source.set_context(Box::into_raw(context).cast());
            source.set_event_handler_f(pressure);
            source.set_finalizer_f(finalize);
            source.activate();
            Self(source)
        }
    }
}
impl Drop for MemoryPressureWatch {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Reads a numeric sysctl value (4- or 8-byte).
pub(crate) fn sysctl_u64(name: &std::ffi::CStr) -> Option<u64> {
    let mut value: u64 = 0;
    let mut size = std::mem::size_of::<u64>();
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&mut value as *mut u64).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0).then_some(match size {
        4 => value & 0xffff_ffff,
        _ => value,
    })
}

/// Installed RAM.
pub fn installed_bytes() -> Option<u64> {
    sysctl_u64(c"hw.memsize")
}

/// Memory macOS considers available: the kernel's own estimate
/// (`kern.memorystatus_level`, the "free percentage" `memory_pressure` reports),
/// which counts reclaimable caches, applied to the installed RAM.
pub fn available_bytes() -> Option<u64> {
    let total = installed_bytes()?;
    let free_percent = sysctl_u64(c"kern.memorystatus_level")?.min(100);
    Some(total / 100 * free_percent)
}

#[cfg(test)]
mod tests {
    #[test]
    fn available_memory_is_a_share_of_installed_ram() {
        let available = super::available_bytes().expect("kernel memory estimate");
        assert!(available > 0 && available < 1 << 44, "{available}");
    }
}
