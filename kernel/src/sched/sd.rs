use core::{fmt::Debug, sync::atomic::{AtomicU64, AtomicUsize, Ordering}};
use crate::{libs::spinlock::{SpinLock, SpinLockGuard}, smp::core::smp_get_processor_id, time::{clocksource::HZ, timer::clock}};
use alloc::sync::Arc;
use crate::libs::cpumask::CpuMask;
use balance::MAX_LOAD_BALANCE_INTERVAL;
use super::{balance, idle::CpuIdleType};

#[derive(Debug)]
/// 各个 level 的 sched domai 是 per CPU 的
pub struct SchedDomain {
    lock: SpinLock<()>,
    lock_on_who: AtomicUsize,
    
    parent: Option<Arc<SchedDomain>>,
    child: Option<Arc<SchedDomain>>,
    span: CpuMask,
    flags: u32,
    /// 各个 cpu 对应的 domain 的 groups 成员指向环形链表中的自己的 cpu group
    groups: Arc<SchedGroup>,
    // 记录了在空闲负载均衡过程中遇到的最大成本值。这个值用于帮助调度器决定是否需要进行负载均衡操作，以及在多大程度上进行负载均衡。
    max_newidle_lb_cost: AtomicU64,
    last_decay_max_lb_cost: AtomicU64,
    // 记录最近的负载均衡操作各个类型的次数
    pub lb_count: [u32; CpuIdleType::MAX_IDLE_TYPES as usize],
    // 记录最近的负载均衡操作各个类型的次数，不包含busy类型
    pub lb_nobusyq: [u32; CpuIdleType::MAX_IDLE_TYPES as usize],
    // 记录最近的负载均衡操作各个类型的次数，不包含busy和idle类型
    pub lb_imbalance: [u32; CpuIdleType::MAX_IDLE_TYPES as usize],
    pub lb_failed: [u32; CpuIdleType::MAX_IDLE_TYPES as usize],
    pub lb_balanced: [u32; CpuIdleType::MAX_IDLE_TYPES as usize],
    pub nr_balance_failed: u32,
    // 基础均衡时间间隔 initialise to 1. units in ms.
    balance_interval: u64,
    // cpu 繁忙情况下，均衡时间间隔需要乘上繁忙因子，其缺省值为32
    busy_factor: u64,
    // 最近在该 sd 上执行均衡操作的时间点
    // 判断 sd 是否需要进行均衡的标准是对比当前 jiffies 值和 last_balance+interval
    last_balance: u64,
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
            groups: Arc::new(SchedGroup::new(CpuMask::new())),
            max_newidle_lb_cost: AtomicU64::new(0),
            last_decay_max_lb_cost: AtomicU64::new(0),
            lb_count: [0; CpuIdleType::MAX_IDLE_TYPES as usize],
            lb_nobusyq: [0; CpuIdleType::MAX_IDLE_TYPES as usize],
            lb_imbalance: [0; CpuIdleType::MAX_IDLE_TYPES as usize],
            lb_failed: [0; CpuIdleType::MAX_IDLE_TYPES as usize],
            lb_balanced: [0; CpuIdleType::MAX_IDLE_TYPES as usize],
            nr_balance_failed: 0,
            // initialise to 1. units in ms.
            balance_interval: 1,
            busy_factor: 32,
            last_balance: 0,
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

    pub fn groups(&self) -> &Arc<SchedGroup> {
        &self.groups
    }
    pub fn max_newidle_lb_cost(&self) -> &AtomicU64 {
        &self.max_newidle_lb_cost
    }

    pub fn set_last_balance(&mut self, last_balance: u64) {
        self.last_balance = last_balance;
    }
    pub fn last_balance(&self) -> u64 {
        self.last_balance
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

    pub fn update_newidle_cost(&self, cost: u64) -> bool {
        let current_jiffies = clock(); 

        // 使用 fetch_update 确保并发安全
        let updated = self.max_newidle_lb_cost.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current_max| {
            if cost > current_max {
                Some(cost)
            } else {
                None
            }
        }).is_ok();

        if updated {
            self.last_decay_max_lb_cost.store(current_jiffies, Ordering::SeqCst);
        }else{
            // 确保并发安全的衰减逻辑
            let decay_updated = self.last_decay_max_lb_cost.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |last_decay| {
                log::info!("current_jiffies: {}, last_decay: {}, HZ: {}", current_jiffies, last_decay, HZ);
                if current_jiffies > last_decay + HZ {
                    let mut current_max = self.max_newidle_lb_cost.load(Ordering::SeqCst);
                    loop {
                        let new_cost = current_max.saturating_mul(253).saturating_div(256);
                        match self.max_newidle_lb_cost.compare_exchange(current_max, new_cost, Ordering::SeqCst, Ordering::SeqCst) {
                            Ok(_) => break,
                            Err(x) => current_max = x,
                        }
                    }
                    Some(current_jiffies)
                } else {
                    None
                }
            }).is_ok();
            if decay_updated {
            self.last_decay_max_lb_cost.store(current_jiffies, Ordering::SeqCst);
            return true;
            }
        }
        return false;
    }

    pub fn balance_interval(&self, cpu_busy: CpuIdleType) -> u64 {
        let mut interval = self.balance_interval;
        if cpu_busy == CpuIdleType::NotIdle {
            interval *= self.busy_factor;
            // 将毫秒（ms）转换为jiffies
            interval = interval * HZ / 1000;
            interval -= 1; // 避免高层域的繁忙平衡与低层域的平衡周期相互竞争
        }
        // 限制平衡间隔的最大值
        interval.clamp(1, MAX_LOAD_BALANCE_INTERVAL)
    }
}

#[derive(Debug)]
pub struct SchedGroup {
    /// sched domain 中的所有 sched group 会形成环形链表，next 指向 groups 链表中的下一个节点。
    next: Option<Arc<SchedGroup>>,
    /// 该调度组包括哪些CPU
    cpumask: CpuMask,
    /// 该调度组的算力信息
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

/// sched domain的负载统计信息
struct SdLbStats{
    /// 该 sd 中的最繁忙的 sched group（非 local group）
    busiest: Arc<SchedGroup>,
    /// dest cpu 所在的 sched group
    local: Arc<SchedGroup>,
    total_load: u64,
    total_capacity: u64,
    avg_load: u64,
    busiest_stats: SgLbStats,
    local_stats: SgLbStats,
}

/// sched group的负载统计信息
struct SgLbStats{
    // 只有在 overloaded 时才会计算该值
    avg_load: u64,
    group_load: u64,
    group_capacity: u64,
    
}
