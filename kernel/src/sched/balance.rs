use alloc::sync::Arc;
use system_error::SystemError;
use unified_init::macros::unified_init;
use crate::{exception::softirq::{softirq_vectors, SoftirqNumber}, init::initcall::INITCALL_SUBSYS, time::clocksource::HZ};
use super::fair::DoRebalanceSoftirq;

#[unified_init(INITCALL_SUBSYS)]
/// 注册多核负载均衡的软中断方法
fn register_rebalance_softirq() -> Result<(), SystemError> {
    let sched_softirq = Arc::new(DoRebalanceSoftirq::new());
    softirq_vectors().register_softirq(SoftirqNumber::SCHED, sched_softirq)
        .expect("Failed to register rebalance softirq");
    Ok(())
}

pub const LBF_ALL_PINNED: u32 = 0x01;
pub const LBF_NEED_BREAK: u32 = 0x02;
pub const LBF_DST_PINNED: u32 = 0x04;
pub const LBF_SOME_PINNED: u32 = 0x08;
pub const LBF_ACTIVE_LB: u32 = 0x10;

pub const SYSCTL_SCHED_NR_MIGRATE: u32 = 32;
pub const SCHED_NR_MIGRATE_BREAK: u32 = 8;

pub const MAX_LOAD_BALANCE_INTERVAL: u64 = HZ / 10;
pub const SYSCTL_SCHED_MIGRATION_COST: u64 = 500000;