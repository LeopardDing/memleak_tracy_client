use lazy_static::lazy_static;
use libc::c_char;
use libc::{c_void, dlsym, size_t, RTLD_NEXT};
use std::cell::Cell;
use tracy_client::sys::___tracy_emit_memory_alloc_callstack;
use tracy_client::sys::___tracy_emit_memory_free_callstack;
use tracy_client::Client;

/// 原始malloc函数指针类型
type MallocFn = unsafe extern "C" fn(size: size_t) -> *mut c_void;
/// 原始free函数指针类型
type FreeFn = unsafe extern "C" fn(ptr: *mut c_void);
/// 原始realloc函数指针类型
type ReallocFn = unsafe extern "C" fn(ptr: *mut c_void, size: size_t) -> *mut c_void;
/// 原始calloc函数指针类型
type CallocFn = unsafe extern "C" fn(count: size_t, size: size_t) -> *mut c_void;

lazy_static! {
    /// 原始malloc函数指针
    static ref ORIGINAL_MALLOC: MallocFn = unsafe {
        let ptr = dlsym(RTLD_NEXT, b"malloc\0".as_ptr() as *const c_char);
        if ptr.is_null() {
            panic!("Failed to resolve original malloc");
        }
        std::mem::transmute(ptr)
    };

    /// 原始free函数指针
    static ref ORIGINAL_FREE: FreeFn = unsafe {
        let ptr = dlsym(RTLD_NEXT, b"free\0".as_ptr() as *const c_char);
        if ptr.is_null() {
            panic!("Failed to resolve original free");
        }
        std::mem::transmute(ptr)
    };

    /// 原始realloc函数指针
    static ref ORIGINAL_REALLOC: ReallocFn = unsafe {
        let ptr = dlsym(RTLD_NEXT, b"realloc\0".as_ptr() as *const c_char);
        if ptr.is_null() {
            panic!("Failed to resolve original realloc");
        }
        std::mem::transmute(ptr)
    };

    /// 原始calloc函数指针
    static ref ORIGINAL_CALLOC: CallocFn = unsafe {
        let ptr = dlsym(RTLD_NEXT, b"calloc\0".as_ptr() as *const c_char);
        if ptr.is_null() {
            panic!("Failed to resolve original calloc");
        }
        std::mem::transmute(ptr)
    };
}

// ============================================================================
// 递归防护机制
// ============================================================================

thread_local! {
    /// 递归调用防护标志：防止在hook中再次调用malloc导致无限递归
    static RECURSION_GUARD: Cell<u32> = Cell::new(0);
}

/// 进入临界区，增加递归计数
#[inline]
fn enter_critical_section() -> bool {
    RECURSION_GUARD.with(|guard| {
        let current = guard.get();
        if current > 0 {
            // 已经在hook中，直接返回false，调用原始malloc
            false
        } else {
            guard.set(1);
            true
        }
    })
}

/// 离开临界区，减少递归计数
#[inline]
fn exit_critical_section() {
    RECURSION_GUARD.with(|guard| {
        guard.set(0);
    });
}

/// 全局内存追踪器
pub struct AllocationTracker {
    client: Client,
}

impl AllocationTracker {
    /// 创建新的追踪器
    pub fn new() -> Self {
        AllocationTracker {
            client: Client::start(),
        }
    }

    pub fn message(&self, msg: &str) {
        self.client.message(msg, 60);
    }
}

impl Default for AllocationTracker {
    fn default() -> Self {
        Self::new()
    }
}

lazy_static! {
    /// 全局内存追踪器实例
    static ref GLOBAL_TRACKER: AllocationTracker = {
        let tracker = AllocationTracker::new();
        tracker
    };
}

// ============================================================================
// Hook函数导出 - C兼容符号
// ============================================================================

const CALLSTACK_DEPTH: i32 = 5;

/// 拦截malloc函数
#[no_mangle]
pub extern "C" fn malloc(size: size_t) -> *mut c_void {
    if !enter_critical_section() {
        // 递归调用，直接转发给原始malloc
        return unsafe { (*ORIGINAL_MALLOC)(size) };
    }

    let ptr = unsafe {
        let ptr = (*ORIGINAL_MALLOC)(size);
        ___tracy_emit_memory_alloc_callstack(ptr, size, CALLSTACK_DEPTH, 0);
        ptr
    };

    exit_critical_section();
    ptr
}

/// 拦截free函数
#[no_mangle]
pub extern "C" fn free(ptr: *mut c_void) {
    if !enter_critical_section() {
        unsafe { (*ORIGINAL_FREE)(ptr) };
        return;
    }

    unsafe {
        ___tracy_emit_memory_free_callstack(ptr, CALLSTACK_DEPTH, 0);
        (*ORIGINAL_FREE)(ptr)
    };

    exit_critical_section();
}

/// 拦截realloc函数
#[no_mangle]
pub extern "C" fn realloc(ptr: *mut c_void, size: size_t) -> *mut c_void {
    if !enter_critical_section() {
        return unsafe { (*ORIGINAL_REALLOC)(ptr, size) };
    }

    let new_ptr = unsafe {
        // 如果旧指针不为空，记录释放事件
        if !ptr.is_null() {
            ___tracy_emit_memory_free_callstack(ptr, CALLSTACK_DEPTH, 0);
        }

        let new_ptr = (*ORIGINAL_REALLOC)(ptr, size);

        // 只在新分配成功时记录分配事件
        if !new_ptr.is_null() {
            ___tracy_emit_memory_alloc_callstack(new_ptr, size, CALLSTACK_DEPTH, 0);
        }
        new_ptr
    };

    exit_critical_section();
    new_ptr
}

/// 拦截calloc函数
#[no_mangle]
pub extern "C" fn calloc(count: size_t, size: size_t) -> *mut c_void {
    if !enter_critical_section() {
        return unsafe { (*ORIGINAL_CALLOC)(count, size) };
    }

    let ptr = unsafe {
        let ptr = (*ORIGINAL_CALLOC)(count, size);
        let total_size = count.saturating_mul(size);
        if !ptr.is_null() {
            ___tracy_emit_memory_alloc_callstack(ptr, total_size, CALLSTACK_DEPTH, 0);
        }
        ptr
    };
    exit_critical_section();
    ptr
}

// ============================================================================
// 库初始化和清理
// ============================================================================

#[ctor::ctor]
fn init() {
    eprintln!("[memleak] Library loaded");
}

#[ctor::dtor]
fn finalize() {
    eprintln!("[memleak] Library unloaded");
}
