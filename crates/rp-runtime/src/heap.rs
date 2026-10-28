//! 堆内存统计：可开关的计数分配器。
//!
//! 默认不计账（`alloc` 里只有一次原子读），在配置了堆上限或调用
//! `sys.heap_usage()` 时开启。计数是「统计期内申请总量 − 释放总量」，
//! 因此开启前分配、开启后才释放的内存可能让数字偏低——作为容量上限与
//! 泄漏趋势判断足够，不用于精确计费。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static TRACKING: AtomicBool = AtomicBool::new(false);
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
static FREED: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// 计数分配器；在 `lib.rs` 里注册为全局分配器。
pub struct CountingAllocator;

impl CountingAllocator {
    fn record_alloc(bytes: usize) {
        let total = ALLOCATED.fetch_add(bytes, Ordering::Relaxed) + bytes;
        let used = total.saturating_sub(FREED.load(Ordering::Relaxed));
        PEAK.fetch_max(used, Ordering::Relaxed);
    }

    fn record_free(bytes: usize) {
        FREED.fetch_add(bytes, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() && TRACKING.load(Ordering::Relaxed) {
            Self::record_alloc(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc_zeroed(layout);
        if !ptr.is_null() && TRACKING.load(Ordering::Relaxed) {
            Self::record_alloc(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if TRACKING.load(Ordering::Relaxed) {
            Self::record_free(layout.size());
        }
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = System.realloc(ptr, layout, new_size);
        if !new_ptr.is_null() && TRACKING.load(Ordering::Relaxed) {
            if new_size >= layout.size() {
                Self::record_alloc(new_size - layout.size());
            } else {
                Self::record_free(layout.size() - new_size);
            }
        }
        new_ptr
    }
}

/// 开启统计（幂等）；返回此前是否已开启。
pub fn enable() -> bool {
    TRACKING.swap(true, Ordering::Relaxed)
}

pub fn enabled() -> bool {
    TRACKING.load(Ordering::Relaxed)
}

/// 统计期内的净堆占用（字节）。
pub fn usage() -> usize {
    ALLOCATED
        .load(Ordering::Relaxed)
        .saturating_sub(FREED.load(Ordering::Relaxed))
}

/// 统计期内的峰值占用（字节）。
pub fn peak() -> usize {
    PEAK.load(Ordering::Relaxed)
}

/// 把峰值重置为当前占用。
pub fn reset_peak() {
    PEAK.store(usage(), Ordering::Relaxed);
}
