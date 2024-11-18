use core::{fmt::Debug, sync::atomic::{AtomicUsize, Ordering}};
use crate::{libs::spinlock::{SpinLock, SpinLockGuard}, smp::core::smp_get_processor_id};
use alloc::sync::Arc;
use crate::libs::cpumask::CpuMask;
use alloc::vec::Vec;
use super::idle::CpuIdleType;

#[derive(Debug)]
pub struct SchedDomain {
    lock: SpinLock<()>,
    lock_on_who: AtomicUsize,
    
    parent: Option<Arc<SchedDomain>>,
    child: Option<Arc<SchedDomain>>,
    span: CpuMask,
    flags: u32,
    groups: Vec<Arc<SchedGroup>>,
    // 记录最近的负载均衡操作的成本，用于限制负载均衡的频率
    max_newidle_lb_cost: u32,
    // 记录最近的负载均衡操作各个类型的次数
    pub lb_count: [u32; CpuIdleType::MAX_IDLE_TYPES as usize],
    // 记录最近的负载均衡操作各个类型的次数，不包含busy类型
    pub lb_nobusyq: [u32; CpuIdleType::MAX_IDLE_TYPES as usize],
    // 记录最近的负载均衡操作各个类型的次数，不包含busy和idle类型
    pub lb_imbalance: [u32; CpuIdleType::MAX_IDLE_TYPES as usize],
    pub lb_failed: [u32; CpuIdleType::MAX_IDLE_TYPES as usize],
    pub lb_balanced: [u32; CpuIdleType::MAX_IDLE_TYPES as usize],
    pub nr_balance_failed: u32,
}

impl SchedDomain {
    pub fn new(span: CpuMask) -> Self {
        Self {
            lock: SpinLock::new(()),
            lock_on_who: AtomicUsize::new(usize::MAX),
            parent: None,
            child: None,
            span,
            flags: 0,
            groups: Vec::new(),
            max_newidle_lb_cost: 0,
            lb_count: [0; CpuIdleType::MAX_IDLE_TYPES as usize],
            lb_nobusyq: [0; CpuIdleType::MAX_IDLE_TYPES as usize],
            lb_imbalance: [0; CpuIdleType::MAX_IDLE_TYPES as usize],
            lb_failed: [0; CpuIdleType::MAX_IDLE_TYPES as usize],
            lb_balanced: [0; CpuIdleType::MAX_IDLE_TYPES as usize],
            nr_balance_failed: 0,
        }
    }

    pub fn parent(&self) -> Option<Arc<SchedDomain>> {
        self.parent.clone()
    }

    pub fn set_parent(&mut self, parent: Option<Arc<SchedDomain>>) {
        self.parent = parent;
    }

    pub fn child(&self) -> Option<Arc<SchedDomain>> {
        self.child.clone()
    }

    pub fn set_child(&mut self, child: Option<Arc<SchedDomain>>) {
        self.child = child;
    }

    pub fn span(&self) -> &CpuMask {
        &self.span
    }

    pub fn groups(&self) -> &Vec<Arc<SchedGroup>> {
        &self.groups
    }
    pub fn max_newidle_lb_cost(&self) -> u32 {
        self.max_newidle_lb_cost
    }

    /// 此函数只能在关中断的情况下使用！！！
    /// 获取到 sd 的可变引用，需要注意的是返回的第二个值需要确保其生命周期
    /// 所以可以说这个函数是unsafe的，需要确保正确性
    /// 在中断上下文，关中断的情况下，此函数是安全的
    pub fn self_lock(&self) -> (&mut Self, Option<SpinLockGuard<()>>) {
        if self.lock.is_locked()
            && smp_get_processor_id().data() as usize == self.lock_on_who.load(Ordering::SeqCst)
        {
            // 在本cpu已上锁则可以直接拿
            (
                unsafe {
                    (self as *const Self as usize as *mut Self)
                        .as_mut()
                        .unwrap()
                },
                None,
            )
        } else {
            // 否则先上锁再拿
            let guard = self.lock();
            (
                unsafe {
                    (self as *const Self as usize as *mut Self)
                        .as_mut()
                        .unwrap()
                },
                Some(guard),
            )
        }
    }

    fn lock(&self) -> SpinLockGuard<()> {
        let guard = self.lock.lock_irqsave();
        self.lock_on_who.store(smp_get_processor_id().data() as usize, Ordering::SeqCst);
        guard
    }
}

#[derive(Debug)]
pub struct SchedGroup {
    next: Option<Arc<SchedGroup>>,
    cpumask: CpuMask,
    sgc: SchedGroupCapacity,
}
impl SchedGroup {
    pub fn new(cpumask: CpuMask) -> Self {
        Self {
            next: None,
            cpumask,
            sgc: SchedGroupCapacity::new(),
        }
    }

    pub fn next(&self) -> Option<Arc<SchedGroup>> {
        self.next.clone()
    }

    pub fn set_next(&mut self, next: Option<Arc<SchedGroup>>) {
        self.next = next;
    }

    pub fn cpumask(&self) -> &CpuMask {
        &self.cpumask
    }
    
    pub fn sgc(&self) -> &SchedGroupCapacity {
        &self.sgc
    }
}

#[derive(Debug)]
pub struct SchedGroupCapacity {
    /// CPU capacity of this group, SCHED_CAPACITY_SCALE being max capacity for a single CPU
    capacity: u64,
    min_capacity: u64,
    max_capacity: u64,
    next_update: u64,
    imbalance: i32,
    id: i32,
    cpumask: CpuMask,
}

impl SchedGroupCapacity {
    pub fn new() -> Self {
        Self {
            capacity: 0,
            min_capacity: 0,
            max_capacity: 0,
            next_update: 0,
            imbalance: 0,
            id: 0,
            cpumask: CpuMask::new(),
        }
    }

    pub fn imbalance(&self) -> i32 {
        self.imbalance
    }
}